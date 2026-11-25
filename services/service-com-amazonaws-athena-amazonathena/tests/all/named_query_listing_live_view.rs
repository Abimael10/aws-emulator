// SPDX-License-Identifier: CC0-1.0

//! `ListNamedQueries` is a LIVE VIEW of the saved queries a workgroup holds, driven end to end
//! over the same router the emulator boots (awsJson1_1,
//! `x-amz-target: AmazonAthena.<Operation>`), so every assertion is about what an SDK client
//! observes.
//!
//! THE RULE UNDER TEST: **every answer of the listing is computed from the registry as it stands
//! when the call arrives — so the ids it reports are exactly those of the queries the named
//! workgroup holds at that moment (the account's default workgroup where the request names
//! none), a create adds its id, a delete takes its id away, a revision of a query changes no
//! membership, and the read itself changes nothing a later caller sees.**
//!
//! The rows differ along what the rule generalises over: the mutation applied (a save, a
//! revision, a delete), how many queries the workgroup holds at that point (none, one, several,
//! and back to none), which workgroup is listed (one a create registered, one that never holds a
//! query, and the default workgroup a request naming none addresses) and the page size the walk
//! asks for (none, one, two, and the modelled maximum) — the set a walk reports may not depend
//! on any of those.
//!
//! Sources: `MaxNamedQueriesCount` is modelled `range 0..50` and "if a workgroup is not
//! specified, lists the saved queries for the primary workgroup" is `ListNamedQueries`'s own
//! documentation
//! (`smithy_query(query="operation",operation="ListNamedQueries")`); `UpdateNamedQuery` states
//! that "the database or workgroup cannot be updated", which is why a revision moves no query
//! between the listings.
//!
//! No expected value is read out of the answer being checked: every id asserted on is one
//! `CreateNamedQuery` answered with.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, create_work_group, router};

/// The workgroup a request naming none addresses.
const DEFAULT_WORK_GROUP: &str = "primary";

/// A workgroup the creates below register queries in.
const REPORTS: &str = "live-view-reports";

/// A workgroup that is created and never holds a saved query, so every listing of it is the
/// empty view.
const BESIDE: &str = "live-view-beside";

/// The largest page the modelled `MaxNamedQueriesCount` (`range 0..50`) admits.
const MAX_PAGE: i64 = 50;

/// The page sizes every listing here is walked at: a request naming none, one id a page, two,
/// and the modelled maximum.
const PAGE_SIZES: &[Option<i64>] = &[None, Some(1), Some(2), Some(MAX_PAGE)];

/// One mutation of this account's saved queries, as a client makes it.
#[derive(Debug, Clone, Copy)]
enum Step<'a> {
    /// Save a new query under `name` in `work_group` (`None` naming none).
    Save {
        name: &'a str,
        work_group: Option<&'a str>,
    },
    /// Revise the query saved under `name`, which changes no membership.
    Revise { name: &'a str },
    /// Delete the query saved under `name`.
    Forget { name: &'a str },
}

/// The queries this account holds, by the name they were saved under, as
/// `(the id the create minted, the workgroup it belongs to)`.
type Held<'a> = BTreeMap<&'a str, (String, String)>;

/// Save one query and answer with the `NamedQueryId` Athena minted for it.
async fn save(router: &MultiServiceRouter, name: &str, work_group: Option<&str>) -> String {
    let mut request = json!({
        "Name": name,
        "Database": "default",
        "QueryString": "SELECT 1",
    });
    if let Some(work_group) = work_group {
        request["WorkGroup"] = json!(work_group);
    }
    let (status, saved) = call(router, "CreateNamedQuery", &request).await;
    assert_eq!(
        status, 200,
        "saving {name:?}: expected HTTP 200, got {status} with {saved:?}"
    );
    saved["NamedQueryId"]
        .as_str()
        .unwrap_or_else(|| panic!("saving {name:?}: expected a minted id, got {saved:?}"))
        .to_string()
}

/// Revise the query `id` names, as a client editing a saved query does.
async fn revise(router: &MultiServiceRouter, id: &str, name: &str) {
    let (status, answered) = call(
        router,
        "UpdateNamedQuery",
        &json!({
            "NamedQueryId": id,
            "Name": name,
            "QueryString": "SELECT 2",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "revising {name:?}: expected HTTP 200, got {status} with {answered:?}"
    );
}

/// Delete the query `id` names.
async fn forget(router: &MultiServiceRouter, id: &str, name: &str) {
    let (status, answered) = call(router, "DeleteNamedQuery", &json!({ "NamedQueryId": id })).await;
    assert_eq!(
        status, 200,
        "deleting {name:?}: expected HTTP 200, got {status} with {answered:?}"
    );
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
        // Every element must BE an id: an element of another shape is a wrong answer, so it
        // fails here rather than disappearing out of the set being compared.
        .map(|id| {
            id.as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected every reported id to be a string, got {id:?}")
                })
                .to_string()
        })
        .collect();
    (ids, listed["NextToken"].as_str().map(str::to_string))
}

/// Every id the listing reports for `work_group` when it is walked `page_size` at a time,
/// following the tokens it hands back, sorted so the comparison is of the SET reported.
async fn walked(
    router: &MultiServiceRouter,
    work_group: Option<&str>,
    page_size: Option<i64>,
    case: &str,
) -> Vec<String> {
    let mut collected: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    // One look per page the workgroup could hold, plus one: a walk that has not ended by then is
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
            None => {
                collected.sort();
                return collected;
            }
            Some(next) => token = Some(next),
        }
    }
    panic!("case {case:?}: the walk never reached the last page, collected {collected:?}");
}

/// Assert that each of the three workgroups lists exactly the queries `held` says it holds —
/// walked at every page size, and, for the default workgroup, both named and unnamed.
///
/// Every walk re-lists what the walk before it already listed, so a listing that consumed
/// anything a later caller sees — a page, a quota, a cursor — would answer differently here the
/// second time.
async fn assert_live(router: &MultiServiceRouter, held: &Held<'_>, case: &str) {
    for work_group in [DEFAULT_WORK_GROUP, REPORTS, BESIDE] {
        let mut expected: Vec<String> = held
            .values()
            .filter(|(_, held_in)| held_in.as_str() == work_group)
            .map(|(id, _)| id.clone())
            .collect();
        expected.sort();
        // The default workgroup is addressed both ways: named, and by a request naming none.
        let namings: Vec<Option<&str>> = if work_group == DEFAULT_WORK_GROUP {
            vec![Some(DEFAULT_WORK_GROUP), None]
        } else {
            vec![Some(work_group)]
        };
        for naming in namings {
            for page_size in PAGE_SIZES {
                let case = format!(
                    "{case}: {work_group} as {naming:?}, walked {page_size:?} ids a page"
                );
                let actual = walked(router, naming, *page_size, &case).await;
                assert_eq!(
                    actual, expected,
                    "case {case:?}: expected the queries the workgroup holds right now \
                     ({expected:?}), got {actual:?}"
                );
            }
        }
    }
}

/// The listing follows the registry: it reports no id before any create, exactly the ids the
/// creates left behind at every point in a sequence of saves, revisions and deletes, and no id
/// again once the last query is gone.
#[tokio::test]
async fn the_listing_reports_the_queries_held_when_the_call_arrives() {
    let router = router();
    create_work_group(&router, REPORTS).await;
    create_work_group(&router, BESIDE).await;

    // The steps differ along the mutation applied, the workgroup it touches and the count it
    // leaves behind — from none held, through several, and back to none.
    let steps: &[Step<'_>] = &[
        Step::Save {
            name: "first",
            work_group: None,
        },
        Step::Save {
            name: "second",
            work_group: Some(DEFAULT_WORK_GROUP),
        },
        Step::Save {
            name: "reported-first",
            work_group: Some(REPORTS),
        },
        Step::Save {
            name: "third",
            work_group: None,
        },
        // A revision changes the query, never which workgroup lists it.
        Step::Revise { name: "second" },
        Step::Forget { name: "first" },
        Step::Save {
            name: "reported-second",
            work_group: Some(REPORTS),
        },
        Step::Forget { name: "reported-first" },
        Step::Forget { name: "second" },
        Step::Forget { name: "third" },
        Step::Forget { name: "reported-second" },
    ];

    let mut held: Held<'_> = BTreeMap::new();
    assert_live(&router, &held, "before any query is saved").await;

    for step in steps {
        match *step {
            Step::Save { name, work_group } => {
                let id = save(&router, name, work_group).await;
                held.insert(
                    name,
                    (id, work_group.unwrap_or(DEFAULT_WORK_GROUP).to_string()),
                );
            }
            Step::Revise { name } => {
                let (id, _) = held
                    .get(name)
                    .unwrap_or_else(|| panic!("the step revises {name:?}, which was saved above"))
                    .clone();
                revise(&router, &id, name).await;
            }
            Step::Forget { name } => {
                let (id, _) = held
                    .remove(name)
                    .unwrap_or_else(|| panic!("the step deletes {name:?}, which was saved above"));
                forget(&router, &id, name).await;
            }
        }
        assert_live(&router, &held, &format!("after {step:?}")).await;
    }

    assert!(
        held.is_empty(),
        "the sequence must end with nothing held, left {held:?}"
    );
}
